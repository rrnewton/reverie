/* Is a sleep denominated in global virtual time, and does that make its length
 * depend on how many OTHER threads are running?
 *
 * This is the scheduling-coupling test. A sleep deadline is computed as
 * `thread_observe_time() + delta`, i.e. in units of the global clock, and the
 * scheduler releases the sleeper when its committed time (also the global
 * clock) passes that deadline. If the global clock is a SUM over threads, then
 * adding busy siblings makes it advance faster, so a fixed sleep should expire
 * after LESS work by the sleeping thread's own reckoning.
 *
 * Thread 0 sleeps for a fixed virtual duration while N-1 siblings spin. It
 * reports how much of its own rdtsc elapsed across the sleep, and how much
 * global time did.
 *
 * If the two columns move together as N grows, sleeps are insensitive to
 * sibling count. If global stays fixed while rdtsc shrinks, the sleep's real
 * cost in program progress depends on the sibling count -- which is what a
 * change from SUM to MAX would alter.
 *
 * Build: gcc -O2 -pthread -o sleepscale sleepscale.c
 * Usage: sleepscale <threads> <sleep-ms>
 */
#define _GNU_SOURCE
#include <pthread.h>
#include <stdatomic.h>
#include <stdio.h>
#include <stdlib.h>
#include <time.h>
#include <x86intrin.h>

static atomic_int stop_flag;

static double monotonic_seconds(void)
{
    struct timespec t;
    clock_gettime(CLOCK_MONOTONIC, &t);
    return (double)t.tv_sec + (double)t.tv_nsec / 1e9;
}

static void *spinner(void *arg)
{
    (void)arg;
    volatile unsigned long sink = 0;
    while (!atomic_load_explicit(&stop_flag, memory_order_relaxed))
        sink += 2654435761u;
    return NULL;
}

int main(int argc, char **argv)
{
    int threads = argc > 1 ? atoi(argv[1]) : 1;
    long sleep_ms = argc > 2 ? atol(argv[2]) : 50;

    pthread_t *ids = calloc((size_t)threads, sizeof(pthread_t));
    for (int i = 1; i < threads; i++)
        pthread_create(&ids[i], NULL, spinner, NULL);

    /* Let the siblings get going so the sleep overlaps steady-state spinning. */
    struct timespec warmup = {0, 5000000};
    nanosleep(&warmup, NULL);

    double mono_before = monotonic_seconds();
    unsigned long long tsc_before = __rdtsc();

    struct timespec request = {sleep_ms / 1000, (sleep_ms % 1000) * 1000000};
    nanosleep(&request, NULL);

    unsigned long long tsc_after = __rdtsc();
    double mono_after = monotonic_seconds();

    atomic_store(&stop_flag, 1);
    for (int i = 1; i < threads; i++)
        pthread_join(ids[i], NULL);

    printf("threads %d slept_ms %ld global_delta %.9f own_tsc_delta %llu\n",
           threads, sleep_ms, mono_after - mono_before, tsc_after - tsc_before);
    free(ids);
    return 0;
}
