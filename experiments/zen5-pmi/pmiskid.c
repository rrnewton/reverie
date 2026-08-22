/* Measure raw PMI skid on this CPU with reverie's exact event configuration:
   raw 0x5100d1, exclude_kernel, exclude_guest, pinned, wakeup_events=1,
   signal-on-overflow. On each overflow signal, read the counter and report
   (actual - programmed_period) = the skid for that interval. No hermit. */
#define _GNU_SOURCE
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>
#include <signal.h>
#include <fcntl.h>
#include <errno.h>
#include <sys/ioctl.h>
#include <sys/syscall.h>
#include <linux/perf_event.h>

static int fd = -1;
static volatile sig_atomic_t hits = 0;
static unsigned long long period;
static long long *skids; static int nskid = 0, maxskid;

static long long read_ctr(void) { long long v = 0; if (read(fd, &v, sizeof v) != sizeof v) return -1; return v; }

static void on_overflow(int sig, siginfo_t *si, void *uc) {
    (void)sig; (void)si; (void)uc;
    long long v = read_ctr();
    long long expected = (long long)period * (hits + 1);
    if (nskid < maxskid) skids[nskid++] = v - expected;
    hits++;
    ioctl(fd, PERF_EVENT_IOC_REFRESH, 1);
}

int main(int argc, char **argv) {
    period = (argc > 1) ? strtoull(argv[1], 0, 0) : 1000000ULL;
    long iters = (argc > 2) ? atol(argv[2]) : 300L;
    maxskid = iters + 16; skids = calloc(maxskid, sizeof *skids);

    struct perf_event_attr a; memset(&a, 0, sizeof a);
    a.type = PERF_TYPE_RAW; a.size = sizeof a; a.config = 0x5100d1;
    a.sample_period = period;
    a.disabled = 1; a.exclude_kernel = 1; a.exclude_guest = 1; a.pinned = 1;
    a.wakeup_events = 1;
    fd = syscall(SYS_perf_event_open, &a, 0, -1, -1, 0);
    if (fd < 0) { perror("perf_event_open"); return 1; }

    struct sigaction sa; memset(&sa, 0, sizeof sa);
    sa.sa_sigaction = on_overflow; sa.sa_flags = SA_SIGINFO | SA_RESTART;
    sigaction(SIGIO, &sa, NULL);
    fcntl(fd, F_SETFL, O_ASYNC); fcntl(fd, F_SETSIG, SIGIO); fcntl(fd, F_SETOWN, getpid());

    ioctl(fd, PERF_EVENT_IOC_RESET, 0);
    ioctl(fd, PERF_EVENT_IOC_REFRESH, 1);

    volatile long c = 0;
    while (hits < iters) { for (int i = 0; i < 1000; i++) c += (c & 1) ? 3 : 1; }

    printf("period=%llu samples=%d\n", period, nskid);
    for (int i = 0; i < nskid; i++) printf("skid %lld\n", skids[i]);
    return 0;
}
