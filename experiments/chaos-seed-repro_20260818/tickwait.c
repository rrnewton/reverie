/* Model the guest pattern that costs the time: one thread busy-waits for a
 * counter that a second thread bumps on a periodic timer. This is what
 * raid6_choose_gen() does against jiffies, and what QEMU's vCPU thread does
 * against the emulated PIT driven by QEMU's main-loop thread.
 *
 * Usage: tickwait <ticks-to-wait-for>
 * Prints total spin iterations, which is the analogue of "emulated guest work
 * per delivered tick".
 */
#define _GNU_SOURCE
#include <pthread.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <time.h>

static volatile unsigned long ticks;
static volatile int done;

static void *ticker(void *unused)
{
    (void)unused;
    struct timespec ms = {0, 1000000}; /* 1 ms, like HZ=1000 jiffies */
    while (!done) {
        nanosleep(&ms, NULL);
        ticks++;
    }
    return NULL;
}

int main(int argc, char **argv)
{
    unsigned long want = argc > 1 ? strtoul(argv[1], NULL, 10) : 20;
    pthread_t t;
    pthread_create(&t, NULL, ticker, NULL);

    unsigned long long spins = 0;
    for (unsigned long i = 0; i < want; i++) {
        unsigned long t0 = ticks;
        while (ticks == t0) {
            spins++;
            __asm__ __volatile__("pause" ::: "memory");
        }
    }
    done = 1;
    pthread_join(t, NULL);
    printf("ticks=%lu spins=%llu spins_per_tick=%llu\n", want, spins,
           spins / (want ? want : 1));
    return 0;
}
