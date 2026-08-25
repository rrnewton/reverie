/* Schedule-sensitive probe for --imprecise-timers record/replay.
 *
 * T threads. Each does N rounds; in each round it burns a fixed number of
 * conditional branches (so RCB cost is predictable and preemption points are
 * spread through the run) and then appends its own id to a shared trace via
 * an atomic fetch-and-add index. The trace IS the realized interleaving.
 *
 * Reports, all schedule-sensitive:
 *   switches  how many times the trace alternates between threads
 *   lost      lost updates on a deliberately racy non-atomic counter
 *   digest    FNV-1a over the whole trace -- the schedule fingerprint
 */
#define _GNU_SOURCE
#include <pthread.h>
#include <stdio.h>
#include <stdlib.h>
#include <stdint.h>

static volatile unsigned long racy;      /* deliberately non-atomic */
static unsigned char *trace;
static volatile unsigned long idx;       /* atomic fetch-add cursor */
static int NTHREAD = 2, NROUND = 2000, BURN = 20000;

static void *worker(void *arg)
{
    long id = (long)arg;
    for (int r = 0; r < NROUND; r++) {
        /* Predictable branch burn: BURN conditional branches. */
        volatile long s = 0;
        for (int i = 0; i < BURN; i++) s += i;
        unsigned long v = racy;          /* racy read  */
        racy = v + 1;                    /* racy write */
        unsigned long k = __sync_fetch_and_add(&idx, 1);
        if (k < (unsigned long)(NTHREAD * NROUND)) trace[k] = (unsigned char)id;
    }
    return NULL;
}

int main(int argc, char **argv)
{
    if (argc > 1) NTHREAD = atoi(argv[1]);
    if (argc > 2) NROUND  = atoi(argv[2]);
    if (argc > 3) BURN    = atoi(argv[3]);
    trace = calloc(NTHREAD * NROUND + 1, 1);
    pthread_t th[64];
    for (long i = 0; i < NTHREAD; i++) pthread_create(&th[i], NULL, worker, (void *)i);
    for (long i = 0; i < NTHREAD; i++) pthread_join(th[i], NULL);

    unsigned long n = idx < (unsigned long)(NTHREAD * NROUND) ? idx : (unsigned long)(NTHREAD * NROUND);
    unsigned long sw = 0;
    for (unsigned long i = 1; i < n; i++) if (trace[i] != trace[i-1]) sw++;
    uint64_t h = 1469598103934665603ULL;                 /* FNV-1a */
    for (unsigned long i = 0; i < n; i++) { h ^= trace[i]; h *= 1099511628211ULL; }
    printf("RACEPROBE threads=%d rounds=%d burn=%d slots=%lu switches=%lu lost=%lu digest=%016lx\n",
           NTHREAD, NROUND, BURN, n, sw, (unsigned long)(NTHREAD * NROUND) - racy, h);
    return 0;
}
