/* Single-threaded branch burner for the arm-B root-thread panic question.
 *
 * raceprobe does all of its branch burning on spawned pthreads, so every skid
 * panic it produces lands in a `spawn_local` guest-thread task. This program
 * burns the same kind of conditional branches on the ROOT guest thread and
 * creates no threads at all, so a preemption timer that overshoots here fires
 * on the task that reverie drives inline via `local_set.run_until`, not on a
 * spawned one.
 *
 * Usage: rootburn [rounds] [burn]
 */
#include <stdio.h>
#include <stdlib.h>
#include <stdint.h>

int main(int argc, char **argv)
{
    int rounds = argc > 1 ? atoi(argv[1]) : 2000;
    int burn = argc > 2 ? atoi(argv[2]) : 20000;
    unsigned long acc = 0;

    for (int r = 0; r < rounds; r++) {
        volatile long s = 0;
        for (int i = 0; i < burn; i++) s += i;
        acc += (unsigned long)s;
    }

    printf("ROOTBURN rounds=%d burn=%d acc=%lu\n", rounds, burn, acc);
    return 0;
}
