#define _GNU_SOURCE
#include <errno.h>
#include <setjmp.h>
#include <signal.h>
#include <string.h>
#include <stdlib.h>
#include <sys/syscall.h>
#include <ucontext.h>
#include <unistd.h>
static int mode;
static volatile sig_atomic_t handled;
static sigjmp_buf escape;
static pid_t expected_pid;
static char bytes[6] = {'Z','Z','Z','Z','Z','Z'};
static void caught(int sig, siginfo_t *si, void *context) {
    ucontext_t *u = context;
    if (sig != SIGUSR1 || si->si_signo != SIGUSR1 || si->si_code != SI_TKILL
        || si->si_pid != expected_pid || si->si_uid != getuid()) _exit(100);
    ++handled;
    long value = u->uc_mcontext.gregs[REG_RAX];
    if (mode <= 1 || mode == 4 || mode == 5) {
        if (value != 3) _exit(101);
    } else if (mode == 2) {
        if (value != -EINTR) _exit(102);
    } else if (mode == 3) {
        if (value != SYS_read) _exit(103);
    } else if (value != expected_pid) _exit(104);
    /* A real handler syscall must see the original callback already finalized. */
    if (syscall(SYS_write, 879, 0, 0) != 1) _exit(105);
    if (mode == 4 || mode == 7) u->uc_mcontext.gregs[REG_RAX] = 1234;
    if (mode == 5 || mode == 8) siglongjmp(escape, 1);
}
int main(int argc, char **argv) {
    if (argc != 2) return 106;
    mode = atoi(argv[1]); expected_pid = getpid();
    struct sigaction sa = {0}; sa.sa_sigaction = caught; sa.sa_flags = SA_SIGINFO;
    if (mode == 1 || mode == 3) sa.sa_flags |= SA_RESTART;
    sigemptyset(&sa.sa_mask); sigaddset(&sa.sa_mask, SIGUSR2);
    if (sigaction(SIGUSR1, &sa, 0)) return 107;
    int fds[2]; if (pipe(fds) || write(fds[1], "ABCDEF", 6) != 6) return 108;
    if (syscall(SYS_write, 880, fds[0], mode) != 0) return 109;
    long result = -99;
    if (!sigsetjmp(escape, 1)) {
        errno = 0;
        result = mode < 6 ? syscall(SYS_read, fds[0], bytes, 6)
                          : syscall(SYS_write, 784, 0, 0);
        if (mode == 5 || mode == 8) return 110; /* escape really happened */
        if (mode <= 1 && result != 3) return 111;
        if (mode == 2 && (result != -1 || errno != EINTR)) return 112;
        if (mode == 3 && result != 6) return 113;
        if ((mode == 4 || mode == 7) && result != 1234) return 114;
        if (mode == 6 && result != expected_pid) return 115;
    }
    if (handled != 1) return 116;
    if (mode <= 1 || mode == 4 || mode == 5) {
        if (memcmp(bytes, "ABCZZZ", 6)) return 117;
        char rest[3]; if (read(fds[0], rest, 3) != 3 || memcmp(rest, "DEF", 3)) return 118;
    } else if (mode == 2) {
        if (memcmp(bytes, "ZZZZZZ", 6) || read(fds[0], bytes, 6) != 6
            || memcmp(bytes, "ABCDEF", 6)) return 119;
    } else if (mode == 3 && memcmp(bytes, "ABCDEF", 6)) return 120;
    sigset_t mask; if (sigprocmask(SIG_SETMASK, 0, &mask)
        || sigismember(&mask, SIGUSR1) || sigismember(&mask, SIGUSR2)) return 121;
    if (close(fds[0]) || close(fds[1])) return 122;
    return 0;
}
