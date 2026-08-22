/* Sample Hermit's two guest-visible time domains at the same point in the same
 * thread, from two threads, and print them interleaved.
 *
 * Domain A: clock_gettime(CLOCK_MONOTONIC) -> detcore guest_clock_time()
 * Domain B: rdtsc                          -> detcore handle_rdtsc_event()
 *
 * Output: one line per sample, "tid seq tsc mono_ns".
 */
#define _GNU_SOURCE
#include <pthread.h>
#include <stdint.h>
#include <stdio.h>
#include <time.h>
#include <unistd.h>
#include <x86intrin.h>

#define SAMPLES 40

struct sample {
    long tid;
    int seq;
    unsigned long long tsc;
    unsigned long long mono;
};

static struct sample buf[2][SAMPLES];

static void *work(void *arg)
{
    long id = (long)arg;
    for (int i = 0; i < SAMPLES; i++) {
        struct timespec ts;
        unsigned long long tsc = __rdtsc();
        clock_gettime(CLOCK_MONOTONIC, &ts);
        buf[id][i].tid = id;
        buf[id][i].seq = i;
        buf[id][i].tsc = tsc;
        buf[id][i].mono = (unsigned long long)ts.tv_sec * 1000000000ull +
                          (unsigned long long)ts.tv_nsec;
        /* a little work so the scheduler has something to interleave */
        for (volatile int k = 0; k < 2000; k++) {
        }
    }
    return NULL;
}

int main(int argc, char **argv)
{
    int nthreads = (argc > 1 && argv[1][0] == '1') ? 1 : 2;
    pthread_t t[2];
    for (long i = 0; i < nthreads; i++)
        pthread_create(&t[i], NULL, work, (void *)i);
    for (int i = 0; i < nthreads; i++)
        pthread_join(t[i], NULL);

    for (int i = 0; i < nthreads; i++)
        for (int j = 0; j < SAMPLES; j++)
            printf("SAMPLE thread=%ld seq=%d tsc=%llu mono=%llu\n",
                   buf[i][j].tid, buf[i][j].seq, buf[i][j].tsc, buf[i][j].mono);
    return 0;
}
